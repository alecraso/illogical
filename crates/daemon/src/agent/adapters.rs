//! The npm adapters Claude Code and Codex run through (#111): whether each
//! is installed on this host, and the `npm install` that installs it, made
//! from the pins in `defs.rs` (the README's commands are checked against
//! these in a test). An install older than the pin is out of date (#335):
//! the same install updates it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::defs::{CLAUDE_ACP, CODEX_ACP, Kind, agents_dir};

/// The oldest Node the adapters run on.
pub const NODE_MAJOR: u32 = 20;

pub struct Adapter {
    pub kind: Kind,
    pub label: &'static str,
    /// The agent's own CLI, whose being on this machine says it's used here
    /// (#335).
    pub cli: &'static str,
    /// Its directory under the agents directory.
    pub dir: &'static str,
    pub bin: &'static str,
    /// `name@version`.
    pub package: &'static str,
    /// Extra `npm install` flags (Codex's optional binaries are 100s of MB
    /// it doesn't use).
    pub flags: &'static str,
}

pub const ADAPTERS: &[Adapter] = &[
    Adapter {
        kind: Kind::Claude,
        label: "Claude Code",
        cli: "claude",
        dir: "claude",
        bin: "claude-agent-acp",
        package: CLAUDE_ACP,
        flags: "",
    },
    Adapter {
        kind: Kind::Codex,
        label: "Codex",
        cli: "codex",
        dir: "codex",
        bin: "codex-acp",
        package: CODEX_ACP,
        flags: "--omit=optional ",
    },
];

pub fn of(kind: Kind) -> Option<&'static Adapter> {
    ADAPTERS.iter().find(|a| a.kind == kind)
}

impl Adapter {
    /// The package's name, without the version.
    pub fn name(&self) -> &'static str {
        self.package.rsplit_once('@').map_or(self.package, |(n, _)| n)
    }

    pub fn pinned(&self) -> &'static str {
        self.package.rsplit_once('@').map_or("", |(_, v)| v)
    }

    /// `npm install` for it, with `agents` the agents directory as written
    /// in a shell (`~/…`, or a quoted path).
    pub fn npm(&self, agents: &str) -> String {
        format!("npm install {}--prefix {agents}/{} {}", self.flags, self.dir, self.package)
    }

    /// Its executable in our agents directory.
    pub fn local_bin(&self, home: &Path) -> PathBuf {
        agents_dir(home).join(self.dir).join("node_modules/.bin").join(self.bin)
    }
}

/// The agents directory as a person would type it: `~/…` under home, else
/// the path, quoted if it needs it.
pub fn shown_dir(home: &Path) -> String {
    let dir = agents_dir(home);
    match dir.strip_prefix(home) {
        Ok(rest) if plain(&rest.display().to_string()) => format!("~/{}", rest.display()),
        _ => quote(&dir.display().to_string()),
    }
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@".contains(c))
}

fn quote(s: &str) -> String {
    if plain(s) { s.to_owned() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// `version` from its package.json, when it's in our directory.
    Installed {
        version: Option<String>,
        on_path: bool,
    },
    Missing,
    /// No Node that runs, or this one (too old).
    NoNode {
        found: Option<String>,
    },
}

pub struct Status {
    pub adapter: &'static Adapter,
    pub state: State,
    /// The command to show (and copy).
    pub npm: String,
    /// Where a Node was found that isn't on the PATH a shell gets (mise's),
    /// to put first when installing.
    pub node_bin: Option<PathBuf>,
}

impl Status {
    pub fn ok(&self) -> bool {
        matches!(self.state, State::Installed { .. })
    }

    /// Installed in our directory, but not the version the pin says (#335):
    /// it still runs, and installing again updates it.
    pub fn outdated(&self) -> bool {
        matches!(&self.state, State::Installed { version: Some(v), .. } if v != self.adapter.pinned())
    }

    /// Nothing to install: it's there, at the pin (or on PATH, which is
    /// the person's own).
    pub fn current(&self) -> bool {
        self.ok() && !self.outdated()
    }

    /// Why the agent can't start, for its block.
    pub fn why(&self) -> String {
        match &self.state {
            State::Installed { .. } => String::new(),
            State::Missing => format!("{}'s adapter isn't installed", self.adapter.label),
            State::NoNode { found: Some(v) } => {
                format!("{}'s adapter needs Node {NODE_MAJOR}+ (this machine has {v})", self.adapter.label)
            }
            State::NoNode { found: None } => format!("{}'s adapter needs Node {NODE_MAJOR}+", self.adapter.label),
        }
    }

    pub fn json(&self) -> Value {
        let a = self.adapter;
        let mut v = json!({
            "kind": a.kind,
            "label": a.label,
            "package": a.name(),
            "pinned": a.pinned(),
            "npm": self.npm,
            "node_major": NODE_MAJOR,
        });
        match &self.state {
            State::Installed { version, on_path } => {
                v["state"] = json!("installed");
                v["version"] = json!(version);
                v["on_path"] = json!(on_path);
                v["outdated"] = json!(self.outdated());
            }
            State::Missing => v["state"] = json!("missing"),
            State::NoNode { found } => {
                v["state"] = json!("no_node");
                v["node"] = json!(found);
            }
        }
        if !self.ok() {
            v["why"] = json!(self.why());
        }
        v
    }
}

/// Where `env`'s PATH (else ours) is.
fn path_of(env: &[(String, String)]) -> String {
    env.iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default()
}

fn which(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| p.is_file())
}

/// The Node an adapter would run with: its `--version`, if it runs.
fn node_version(path: &str) -> Option<String> {
    let node = which("node", path)?;
    let out = Command::new(node).arg("--version").env("PATH", path).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && v.starts_with('v')).then_some(v)
}

fn major(v: &str) -> Option<u32> {
    v.trim_start_matches('v').split('.').next()?.parse().ok()
}

/// Whether `a` can start on this host, with `env` as an agent block gets.
pub fn status(a: &'static Adapter, home: &Path, env: &[(String, String)]) -> Status {
    let mut env = env.to_vec();
    let before = path_of(&env);
    super::with_node_on_path(&mut env, home);
    let path = path_of(&env);
    let node_bin = (path != before).then(|| PathBuf::from(path.split(':').next().unwrap_or_default()));
    let npm = a.npm(&shown_dir(home));
    let state = match node_version(&path) {
        None => State::NoNode { found: None },
        Some(v) if major(&v).is_none_or(|m| m < NODE_MAJOR) => State::NoNode { found: Some(v) },
        Some(_) if a.local_bin(home).exists() => {
            let pkg = agents_dir(home).join(a.dir).join("node_modules").join(a.name()).join("package.json");
            let version = std::fs::read(pkg)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v["version"].as_str().map(str::to_owned));
            State::Installed { version, on_path: false }
        }
        Some(_) if which(a.bin, &path).is_some() => State::Installed { version: None, on_path: true },
        Some(_) => State::Missing,
    };
    Status { adapter: a, state, npm, node_bin }
}

/// Every adapter's status.
pub fn all(home: &Path, env: &[(String, String)]) -> Vec<Value> {
    ADAPTERS.iter().map(|a| status(a, home, env).json()).collect()
}

/// The PATH an install runs with: `env`'s, with the Node an agent block
/// would get put first.
pub fn node_path(home: &Path, env: &[(String, String)]) -> String {
    let mut env = env.to_vec();
    super::with_node_on_path(&mut env, home);
    path_of(&env)
}

/// The install, to run here and wait for (Getting started's one click and
/// `arugula setup`, #335): the same `npm install` an *Install* pane runs,
/// into the real directory, with `path` (`node_path`). `ARUGULA_NPM`
/// replaces npm. `None` when there's no npm on that PATH.
pub fn npm_install(a: &Adapter, home: &Path, path: &str) -> Option<Command> {
    let npm = match std::env::var("ARUGULA_NPM") {
        Ok(other) if !other.is_empty() => PathBuf::from(other),
        _ => which("npm", path)?,
    };
    let mut cmd = Command::new(npm);
    cmd.arg("install")
        .args(a.flags.split_whitespace())
        .arg("--prefix")
        .arg(agents_dir(home).join(a.dir))
        .arg(a.package)
        .env("PATH", path)
        .stdin(std::process::Stdio::null());
    Some(cmd)
}

/// What an *Install* pane runs: the shown command, but into the real
/// directory and with the Node we found first on PATH. `ARUGULA_NPM`
/// replaces npm (the browser tests' stand-in).
pub fn install_command(s: &Status, home: &Path) -> String {
    let npm = s.adapter.npm(&quote(&agents_dir(home).display().to_string()));
    let npm = match std::env::var("ARUGULA_NPM") {
        Ok(other) if !other.is_empty() => format!("{}{}", quote(&other), npm.trim_start_matches("npm")),
        _ => npm,
    };
    let npm = match &s.node_bin {
        // Quoted, `$PATH` is colon-joined in fish too.
        Some(bin) => format!("env \"PATH={}:$PATH\" {npm}", bin.display()),
        None => npm,
    };
    format!("{npm} && echo && echo 'Installed {}. Start the agent again.'", s.adapter.package)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_come_from_the_pins() {
        let claude = of(Kind::Claude).unwrap();
        assert_eq!(claude.name(), "@agentclientprotocol/claude-agent-acp");
        assert_eq!(claude.pinned(), CLAUDE_ACP.rsplit_once('@').unwrap().1);
        assert_eq!(
            claude.npm("~/.local/share/arugula/agents"),
            format!("npm install --prefix ~/.local/share/arugula/agents/claude {CLAUDE_ACP}")
        );
        assert_eq!(
            of(Kind::Codex).unwrap().npm("~/x"),
            format!("npm install --omit=optional --prefix ~/x/codex {CODEX_ACP}")
        );
        assert!(of(Kind::Fountain).is_none());
        assert_eq!(quote("/a b/it's"), r"'/a b/it'\''s'");
        assert_eq!(major("v22.23.2"), Some(22));
        assert_eq!(major("v8.1.0"), Some(8));
    }

    /// The README and features.md carry the install commands by hand: they
    /// must be the ones generated from the pins.
    #[test]
    fn docs_match_the_pins() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for doc in ["README.md", "docs/features.md"] {
            let text = std::fs::read_to_string(root.join(doc)).unwrap();
            // Joined across line breaks, as Markdown would.
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            for a in ADAPTERS {
                let cmd = a.npm("~/.local/share/arugula/agents");
                assert!(text.contains(&cmd), "{doc} should have `{cmd}` (the pin in defs.rs)");
            }
            for pinned in text.match_indices("@agentclientprotocol/").map(|(i, _)| &text[i..]) {
                let pkg: String = pinned.chars().take_while(|c| !c.is_whitespace() && *c != '`').collect();
                if pkg.matches('@').count() == 2 {
                    assert!(ADAPTERS.iter().any(|a| a.package == pkg), "{doc} pins {pkg}, which defs.rs doesn't");
                }
            }
        }
    }

    // Unix: a fake node made executable by its mode.
    #[cfg(unix)]
    #[test]
    fn reports_missing_and_installed() {
        // The agents directory is under home unless this is set.
        if std::env::var_os("ARUGULA_AGENTS_DIR").is_some() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("arugula-adapters-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = &dir;
        let claude = of(Kind::Claude).unwrap();
        // No Node at all on this PATH.
        let bare = vec![("PATH".to_owned(), home.join("nothing").display().to_string())];
        let s = status(claude, home, &bare);
        assert_eq!(s.state, State::NoNode { found: None });
        assert!(s.why().contains("needs Node 20+"));
        assert_eq!(s.npm, format!("npm install --prefix ~/.local/share/arugula/agents/claude {CLAUDE_ACP}"));

        // A Node that says it's 22.
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let node = bin.join("node");
        std::fs::write(&node, "#!/bin/sh\necho v22.1.0\n").unwrap();
        std::fs::set_permissions(&node, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let env = vec![("PATH".to_owned(), bin.display().to_string())];
        let s = status(claude, home, &env);
        assert_eq!(s.state, State::Missing);
        assert_eq!(s.why(), "Claude Code's adapter isn't installed");
        assert_eq!(s.json()["state"], "missing");
        assert_eq!(s.json()["why"], "Claude Code's adapter isn't installed");

        // Installed in our directory, with its version.
        let pkg = agents_dir(home).join("claude/node_modules");
        std::fs::create_dir_all(pkg.join(".bin")).unwrap();
        std::fs::write(pkg.join(".bin/claude-agent-acp"), "").unwrap();
        std::fs::create_dir_all(pkg.join(claude.name())).unwrap();
        std::fs::write(pkg.join(claude.name()).join("package.json"), r#"{"version":"0.85.0"}"#).unwrap();
        let s = status(claude, home, &env);
        assert_eq!(s.state, State::Installed { version: Some("0.85.0".into()), on_path: false });
        assert_eq!(s.outdated(), claude.pinned() != "0.85.0");

        // #335: an older version than the pin is out of date, and says so.
        std::fs::write(pkg.join(claude.name()).join("package.json"), r#"{"version":"0.1.0"}"#).unwrap();
        let s = status(claude, home, &env);
        assert!(s.ok() && s.outdated() && !s.current());
        assert_eq!(s.json()["state"], "installed");
        assert_eq!(s.json()["outdated"], true);
        std::fs::write(pkg.join(claude.name()).join("package.json"), format!(r#"{{"version":"{}"}}"#, claude.pinned()))
            .unwrap();
        let s = status(claude, home, &env);
        assert!(s.current() && s.json()["outdated"] == false);

        // The install runs npm with the pin, into our directory.
        std::fs::write(bin.join("npm"), "").unwrap();
        let cmd = npm_install(claude, home, &bin.display().to_string()).unwrap();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["install", "--prefix", &agents_dir(home).join("claude").display().to_string(), CLAUDE_ACP]);
        assert!(npm_install(claude, home, &home.join("nothing").display().to_string()).is_none());

        // Too old a Node.
        std::fs::write(&node, "#!/bin/sh\necho v18.2.0\n").unwrap();
        let s = status(claude, home, &env);
        assert_eq!(s.state, State::NoNode { found: Some("v18.2.0".into()) });
        assert_eq!(s.why(), "Claude Code's adapter needs Node 20+ (this machine has v18.2.0)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
