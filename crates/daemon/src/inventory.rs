//! #145: which agent harnesses are configured on this machine, from
//! `chant audit --agents`, and so which screen rule sets run here.
//!
//! chant reads the agents' config on disk (Claude Code, codex, Gemini,
//! opencode, Cursor; system and user scope) and prints one document for
//! all of them. The daemon runs it in the background when it starts (it
//! takes about a second), keeps what it said, and runs it again on
//! `POST /api/hosts/self/agents/refresh`, or when a pane starts an agent
//! the last read didn't list (someone ran Claude Code here for the first
//! time), at most once a minute.
//!
//! - **Read:** an agent's screen rules run in panes on this machine only
//!   if chant found that runtime configured. The rest fall back to output
//!   activity, and `describe %N --detection` says why.
//! - **No chant, or it failed:** every rule set runs, as before.
//!
//! Which chant: `$ILLOGICAL_CHANT` (set but empty: don't ask chant, which
//! the test daemons do), else `chant` on the user's shell `PATH` (#74).

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::shellenv::ShellEnv;

/// How long chant may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A pane starting an agent the last read didn't list reads again, at most
/// this often.
const STALE: Duration = Duration::from_secs(60);

/// One agent configuration chant found: one runtime at one scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Site {
    pub id: String,
    pub scope: String,
    pub runtime: String,
    pub root: String,
    /// chant's one line: `1 instruction file · 2 MCP servers · model opus`.
    #[serde(default)]
    pub summary: String,
}

/// What the last read said.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not read yet.
    #[default]
    Reading,
    /// chant answered.
    Read,
    /// `$ILLOGICAL_CHANT` is empty.
    Off,
    /// No chant to ask.
    NoChant,
    /// chant ran but didn't say (an old chant without `--agents`, say).
    Failed,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub state: State,
    /// The chant that was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chant: Option<String>,
    /// Its version, from its document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it was read (ms since the epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_ms: Option<u64>,
    pub sites: Vec<Site>,
    /// chant's notes (scopes it didn't scan).
    pub notes: Vec<String>,
}

impl Snapshot {
    /// Whether the agent with this rule set (`claude`, `codex`; the ids
    /// are chant's runtime names) has its screen read here.
    pub fn runs(&self, agent: &str) -> bool {
        self.state != State::Read || self.sites.iter().any(|s| s.runtime == agent)
    }

    /// The runtimes chant found, once each, in its order.
    pub fn runtimes(&self) -> Vec<&str> {
        let mut out: Vec<&str> = vec![];
        for s in &self.sites {
            if !out.contains(&s.runtime.as_str()) {
                out.push(&s.runtime);
            }
        }
        out
    }
}

/// The part of `chant audit --agents --format json` read here.
#[derive(Deserialize)]
struct Doc {
    subject: Option<String>,
    #[serde(default)]
    tool: Option<Tool>,
    #[serde(default)]
    sites: Vec<Site>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Deserialize)]
struct Tool {
    version: Option<String>,
}

/// chant's document as a snapshot, or why it isn't one.
pub fn parse(out: &str) -> Result<Snapshot, String> {
    let doc: Doc = serde_json::from_str(out).map_err(|e| format!("not chant's agent document: {e}"))?;
    if doc.subject.as_deref() != Some("agent-configuration") {
        return Err(format!("not chant's agent document (subject {:?})", doc.subject.unwrap_or_default()));
    }
    Ok(Snapshot {
        state: State::Read,
        version: doc.tool.and_then(|t| t.version),
        sites: doc.sites,
        notes: doc.notes,
        ..Default::default()
    })
}

pub struct Inventory {
    shell_env: Arc<ShellEnv>,
    home: PathBuf,
    now: Mutex<Snapshot>,
    /// Bumped by each read, so the mux knows to look again.
    generation: AtomicU64,
    /// When the last read started, and whether one is running.
    last: Mutex<(Option<Instant>, bool)>,
}

impl Inventory {
    pub fn new(shell_env: Arc<ShellEnv>, home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            shell_env,
            home,
            now: Default::default(),
            generation: AtomicU64::new(0),
            last: Mutex::new((None, false)),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.now.lock().unwrap().clone()
    }

    pub fn runs(&self, agent: &str) -> bool {
        self.now.lock().unwrap().runs(agent)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Read again in the background (unless a read is running).
    pub fn refresh(self: &Arc<Self>) {
        {
            let mut last = self.last.lock().unwrap();
            if last.1 {
                return;
            }
            *last = (Some(Instant::now()), true);
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.read().await;
        });
    }

    /// Read again now, and wait for it.
    pub async fn refresh_now(self: &Arc<Self>) -> Snapshot {
        *self.last.lock().unwrap() = (Some(Instant::now()), true);
        self.read().await;
        self.snapshot()
    }

    /// An agent the last read didn't list started: read again, if the last
    /// read is a while ago.
    pub fn missing(self: &Arc<Self>, agent: &str) {
        let stale = self.last.lock().unwrap().0.is_none_or(|t| t.elapsed() >= STALE);
        if stale && !self.runs(agent) {
            self.refresh();
        }
    }

    async fn read(&self) {
        let snap = self.ask().await;
        match &snap.state {
            State::Read => info!(runtimes = ?snap.runtimes(), "agent inventory (chant audit --agents)"),
            State::Failed | State::NoChant => {
                warn!(error = snap.error.as_deref().unwrap_or_default(), "no agent inventory; every rule set runs")
            }
            _ => {}
        }
        *self.now.lock().unwrap() = snap;
        self.last.lock().unwrap().1 = false;
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    async fn ask(&self) -> Snapshot {
        let shell = self.shell_env.local().await;
        let path = shell.get("PATH").map(str::to_owned).or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        let chant = match std::env::var("ILLOGICAL_CHANT") {
            Ok(c) if c.is_empty() => return Snapshot { state: State::Off, ..Default::default() },
            Ok(c) => Some(PathBuf::from(c)),
            Err(_) => which("chant", &path),
        };
        let Some(chant) = chant else {
            return Snapshot {
                state: State::NoChant,
                error: Some("no chant on PATH (npm install -g @intentius/chant)".into()),
                ..Default::default()
            };
        };
        let mut c = tokio::process::Command::new(&chant);
        c.args(["audit", "--agents", "--scope", "system,user", "--format", "json", "--fail-on", "none"])
            .current_dir(&self.home)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for (k, v) in &shell.vars {
            c.env(k, v);
        }
        let failed = |error: String| Snapshot {
            state: State::Failed,
            chant: Some(chant.display().to_string()),
            error: Some(error),
            ..Default::default()
        };
        let out = match tokio::time::timeout(TIMEOUT, c.output()).await {
            Err(_) => return failed(format!("chant audit --agents took longer than {}s", TIMEOUT.as_secs())),
            Ok(Err(e)) => return failed(format!("{}: {e}", chant.display())),
            Ok(Ok(out)) => out,
        };
        match parse(&String::from_utf8_lossy(&out.stdout)) {
            Ok(snap) => Snapshot {
                chant: Some(chant.display().to_string()),
                read_ms: Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                ),
                ..snap
            },
            Err(e) => {
                let said = String::from_utf8_lossy(&out.stderr);
                let said = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
                let code = out.status.code().unwrap_or(-1);
                failed(if said.is_empty() { format!("{e} (exit {code})") } else { format!("{said} (exit {code})") })
            }
        }
    }
}

fn which(name: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
        .find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("../tests/fixtures/chant/audit-agents.json");

    #[test]
    fn reads_chants_document() {
        let s = parse(RECORDED).unwrap();
        assert_eq!(s.state, State::Read);
        assert_eq!(s.version.as_deref(), Some("0.95.0"));
        assert_eq!(s.runtimes(), ["claude"]);
        assert_eq!(s.sites[0].scope, "user");
        assert_eq!(s.sites[0].summary, "1 instruction file · model sonnet");
        // Claude Code's rules run; Codex isn't configured, so its don't.
        assert!(s.runs("claude"));
        assert!(!s.runs("codex"));
    }

    #[test]
    fn without_a_read_every_rule_set_runs() {
        for state in [State::Reading, State::Off, State::NoChant, State::Failed] {
            let s = Snapshot { state, ..Default::default() };
            assert!(s.runs("claude") && s.runs("codex"));
        }
        assert!(parse("Usage: chant <command>").is_err());
        assert!(parse(r#"{"schemaVersion":"1.0","findings":[]}"#).is_err());
    }
}
