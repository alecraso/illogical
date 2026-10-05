//! Claude Code conversations on this machine (M33): from a terminal or the
//! desktop app's Code tab, which both write
//! `~/.claude/projects/<slug>/<id>.jsonl`.
//!
//! - **The index** reads only the start and end of each transcript (64 KiB
//!   each; 9 ms for geek's 389 in S20), and again only when a file's size or
//!   mtime changed. It's kept in memory: a cold scan is cheap enough that a
//!   file in the state directory isn't worth its staleness.
//! - **Liveness** is `~/.claude/sessions/<pid>.json`, one per running Claude
//!   Code process, checked against the process's start time so a reused pid
//!   doesn't count. Its ancestors (our panes' and agent blocks' processes),
//!   else its systemd scope, say whether it runs in one of ours.
//! - **The desktop app** also keeps `claude-code-sessions/<account>/<org>/
//!   local_<uuid>.json` per session: its title and whether it's archived.
//!
//! [`convert`] turns a transcript into an agent block's entries.

pub mod branch;
pub mod convert;

use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use illogical_proto::PaneId;
use serde::Serialize;
use serde_json::Value;

/// How much of each end of a transcript the index reads.
const EDGE: u64 = 64 * 1024;

/// Where a conversation was had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The `claude` CLI in a terminal.
    Terminal,
    /// The desktop app's Code tab.
    Desktop,
    /// Anything else: `claude -p`, SDK clients, agent blocks.
    Other,
}

impl Source {
    fn of(entrypoint: &str) -> Self {
        match entrypoint {
            "cli" => Self::Terminal,
            "claude-desktop" => Self::Desktop,
            _ => Self::Other,
        }
    }
}

/// A process holding a conversation now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Live {
    pub pid: u32,
    /// `interactive`, …
    pub kind: String,
    pub entrypoint: String,
    /// `idle`, `busy`, …
    pub status: String,
    /// It runs in this pane of ours.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    /// It's this agent block's own agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<PaneId>,
    /// Where, for people ([`Live::place`]).
    pub place: String,
    /// The directory it runs in (#146: where to resume it).
    #[serde(skip)]
    pub cwd: Option<String>,
}

impl Live {
    /// "Live in pane %4", for people.
    pub fn place(&self) -> String {
        match (self.pane, self.block) {
            (Some(p), _) => format!("open in pane %{p}"),
            (_, Some(b)) => format!("open in agent block %{b}"),
            _ if self.entrypoint == "claude-desktop" => "open in the Claude desktop app".into(),
            _ => format!("open in a terminal (pid {})", self.pid),
        }
    }

    /// Only this daemon's panes and blocks (#77): a scope named for a pane
    /// of another daemon on this machine (a dev or test one) isn't ours.
    pub fn ours(mut self, has: impl Fn(PaneId) -> bool) -> Self {
        self.pane = self.pane.filter(|p| has(*p));
        self.block = self.block.filter(|p| has(*p));
        self.place = self.place();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conversation {
    pub id: String,
    pub path: PathBuf,
    pub cwd: String,
    /// The folder is still there.
    pub cwd_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub source: Source,
    pub entrypoint: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    /// The model of its last reply (a full id, `claude-opus-5-5`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub version: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued_in: Option<String>,
    /// Archived in the desktop app.
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<Live>,
}

/// What the start and end of a transcript say.
#[derive(Debug, Clone, Default)]
struct Head {
    id: String,
    cwd: Option<String>,
    branch: Option<String>,
    entrypoint: Option<String>,
    version: Option<String>,
    created_ms: u64,
    first_prompt: Option<String>,
    last_prompt: Option<String>,
    custom_title: Option<String>,
    agent_name: Option<String>,
    ai_title: Option<String>,
    relocated: Option<String>,
    continued_in: Option<String>,
    forked_from: Option<String>,
    model: Option<String>,
}

/// Where Claude Code and the desktop app keep things.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `~/.claude` (or `$CLAUDE_CONFIG_DIR`).
    pub claude: PathBuf,
    /// The desktop app's (`~/.config/Claude`, or `~/Library/Application
    /// Support/Claude`).
    pub desktop: PathBuf,
}

impl Dirs {
    pub fn from_env() -> Self {
        let home = crate::home();
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        let desktop = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude")
        } else if cfg!(windows) {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(r"AppData\Roaming"))
                .join("Claude")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("Claude")
        };
        Self { claude, desktop }
    }
}

/// The desktop app's own record of a session.
#[derive(Debug, Clone, Default)]
struct DesktopRecord {
    title: Option<String>,
    archived: bool,
}

/// Our terminals' and agent blocks' processes, by pid: a Claude Code
/// process under one of them runs there. Needed where there are no systemd
/// scopes to say so (macOS, #81).
#[derive(Debug, Clone, Default)]
pub struct Ours {
    pub panes: HashMap<u32, PaneId>,
    pub blocks: HashMap<u32, PaneId>,
}

impl Ours {
    /// The pane or agent block `pid` runs under, by its ancestors.
    fn holder(&self, pid: u32) -> Option<(Option<PaneId>, Option<PaneId>)> {
        let mut p = pid;
        for _ in 0..64 {
            if let Some(id) = self.panes.get(&p) {
                return Some((Some(*id), None));
            }
            if let Some(id) = self.blocks.get(&p) {
                return Some((None, Some(*id)));
            }
            p = crate::procinfo::ppid(p).filter(|p| *p > 1)?;
        }
        None
    }
}

pub struct Index {
    dirs: Dirs,
    /// By transcript path: its size and mtime when read, and what it said.
    cache: HashMap<PathBuf, (u64, u64, Head)>,
    /// As last told ([`Index::set_ours`]).
    ours: Ours,
}

impl Index {
    pub fn new(dirs: Dirs) -> Self {
        Self { dirs, cache: HashMap::new(), ours: Ours::default() }
    }

    /// Our panes' and agent blocks' processes now. A listing passes them;
    /// a block that checks who holds its session uses the last ones.
    pub fn set_ours(&mut self, ours: Ours) {
        self.ours = ours;
    }

    /// This daemon's, for its user.
    pub fn global() -> &'static Mutex<Index> {
        static G: OnceLock<Mutex<Index>> = OnceLock::new();
        G.get_or_init(|| Mutex::new(Index::new(Dirs::from_env())))
    }

    /// Every conversation, newest first, with what's live now.
    pub fn scan(&mut self) -> Vec<Conversation> {
        let live = live(&self.dirs.claude.join("sessions"), &self.ours);
        let desktop = desktop_records(&self.dirs.desktop.join("claude-code-sessions"));
        let mut seen = HashMap::new();
        let projects = self.dirs.claude.join("projects");
        for dir in std::fs::read_dir(&projects).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
                let path = f.path();
                if path.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                let Ok(md) = f.metadata() else { continue };
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as u64);
                seen.insert(path.clone(), (md.len(), mtime));
            }
        }
        self.cache.retain(|p, _| seen.contains_key(p));
        let mut out = vec![];
        for (path, (size, mtime)) in seen {
            let fresh = matches!(self.cache.get(&path), Some((s, m, _)) if *s == size && *m == mtime);
            if !fresh {
                let Some(head) = read_head(&path) else { continue };
                self.cache.insert(path.clone(), (size, mtime, head));
            }
            let (_, _, h) = &self.cache[&path];
            if h.first_prompt.is_none() && h.last_prompt.is_none() {
                continue;
            }
            let rec = desktop.get(&h.id).cloned().unwrap_or_default();
            let cwd = h.relocated.clone().or(h.cwd.clone()).unwrap_or_default();
            let entrypoint = h.entrypoint.clone().unwrap_or_default();
            let title = h
                .custom_title
                .clone()
                .or(h.agent_name.clone())
                .or(h.ai_title.clone())
                .or(rec.title.clone())
                .or(h.first_prompt.as_deref().map(|p| short(p, 80)))
                .or(h.last_prompt.as_deref().map(|p| short(p, 80)))
                .unwrap_or_else(|| h.id.clone());
            out.push(Conversation {
                id: h.id.clone(),
                path: path.clone(),
                cwd_exists: !cwd.is_empty() && Path::new(&cwd).is_dir(),
                cwd,
                branch: h.branch.clone().filter(|b| !b.is_empty() && b != "HEAD"),
                source: Source::of(&entrypoint),
                entrypoint,
                title,
                first_prompt: h.first_prompt.as_deref().map(|p| short(p, 400)),
                last_prompt: h.last_prompt.as_deref().map(|p| short(p, 400)),
                model: h.model.clone(),
                version: h.version.clone().unwrap_or_default(),
                created_ms: if h.created_ms > 0 { h.created_ms } else { mtime },
                updated_ms: mtime,
                size,
                forked_from: h.forked_from.clone(),
                continued_in: h.continued_in.clone(),
                archived: rec.archived,
                live: live.get(&h.id).cloned(),
            });
        }
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// One conversation by id, or a unique prefix of one.
    pub fn find(&mut self, id: &str) -> Result<Conversation, String> {
        let all = self.scan();
        if let Some(c) = all.iter().find(|c| c.id == id) {
            return Ok(c.clone());
        }
        let hits: Vec<&Conversation> = all.iter().filter(|c| !id.is_empty() && c.id.starts_with(id)).collect();
        match hits.as_slice() {
            [c] => Ok((*c).clone()),
            [] => Err(format!("no Claude Code conversation {id}")),
            _ => Err(format!("{id} could be {} conversations; give more of the id", hits.len())),
        }
    }

    /// Who holds this session now, if anyone.
    pub fn live_for(&self, session: &str) -> Option<Live> {
        live(&self.dirs.claude.join("sessions"), &self.ours).remove(session)
    }
}

/// The conversation each of our panes' Claude Code holds now, by pane
/// (#146: the session to resume when its hooks didn't say).
pub fn live_in_panes(dirs: &Dirs, ours: &Ours) -> HashMap<PaneId, (String, Option<String>)> {
    live(&dirs.claude.join("sessions"), ours).into_iter().filter_map(|(sid, l)| Some((l.pane?, (sid, l.cwd)))).collect()
}

/// Is a conversation on the default list (`all`: everything)? Terminal and
/// desktop ones, unarchived, whose folder is still there; a desktop one
/// whose scratch workspace went with it stays.
pub fn shown(c: &Conversation, all: bool) -> bool {
    all || (c.source != Source::Other && !c.archived && (c.cwd_exists || c.source == Source::Desktop))
}

fn short(s: &str, max: usize) -> String {
    let s = s.trim();
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= max && line.len() == s.len() {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Read a transcript's two ends.
fn read_head(path: &Path) -> Option<Head> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut start = vec![];
    f.by_ref().take(EDGE).read_to_end(&mut start).ok()?;
    let mut end = vec![];
    if len > EDGE {
        f.seek(SeekFrom::Start(len.saturating_sub(EDGE).max(EDGE))).ok()?;
        f.read_to_end(&mut end).ok()?;
    }
    let mut h = Head { id: path.file_stem()?.to_string_lossy().into_owned(), ..Default::default() };
    // Whole lines only: the first chunk's last line and the second's first
    // may be cut.
    let whole = |b: &[u8], cut_front: bool, cut_back: bool| -> Vec<Value> {
        let mut lines: Vec<&[u8]> = b.split(|c| *c == b'\n').collect();
        if cut_back && !b.ends_with(b"\n") {
            lines.pop();
        }
        if cut_front && !lines.is_empty() {
            lines.remove(0);
        }
        lines.into_iter().filter_map(|l| serde_json::from_slice(l).ok()).collect()
    };
    let first = whole(&start, false, len > EDGE);
    let last = if end.is_empty() { vec![] } else { whole(&end, true, false) };
    for v in first.iter().chain(last.iter()) {
        h.take(v);
    }
    Some(h)
}

impl Head {
    fn take(&mut self, v: &Value) {
        let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        match v["type"].as_str().unwrap_or("") {
            "custom-title" => self.custom_title = s("customTitle").or(self.custom_title.take()),
            "agent-name" => self.agent_name = s("agentName").or(self.agent_name.take()),
            "ai-title" => self.ai_title = s("aiTitle").or(self.ai_title.take()),
            "last-prompt" => self.last_prompt = s("lastPrompt").or(self.last_prompt.take()),
            "relocated" => self.relocated = s("relocatedCwd").or(self.relocated.take()),
            "continued-in" => self.continued_in = s("continuedInSessionId").or(self.continued_in.take()),
            t @ ("user" | "assistant") => {
                if v["isSidechain"] == true {
                    return;
                }
                if self.cwd.is_none() {
                    self.cwd = s("cwd");
                    self.branch = s("gitBranch");
                }
                if self.entrypoint.is_none() {
                    self.entrypoint = s("entrypoint");
                }
                if let Some(ver) = s("version") {
                    self.version = Some(ver);
                }
                if self.created_ms == 0 {
                    self.created_ms = convert::at_ms(v["timestamp"].as_str().unwrap_or(""));
                }
                if self.forked_from.is_none() {
                    self.forked_from = v["forkedFrom"]["sessionId"].as_str().map(str::to_owned);
                }
                if t == "assistant"
                    && let Some(m) = v["message"]["model"].as_str().filter(|m| m.starts_with("claude"))
                {
                    self.model = Some(m.to_owned());
                }
                if t == "user" && self.first_prompt.is_none() && v["isMeta"] != true {
                    self.first_prompt = prompt_text(&v["message"]["content"]);
                }
            }
            _ => {}
        }
    }
}

/// What someone typed, from a user line's content (not tool results,
/// commands or reminders).
fn prompt_text(content: &Value) -> Option<String> {
    let raw = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let t = convert::strip_reminders(&raw);
    (!t.is_empty() && !t.starts_with('<') && !t.starts_with("[Request interrupted")).then_some(t)
}

/// The running Claude Code processes, by session.
fn live(dir: &Path, ours: &Ours) -> HashMap<String, Live> {
    let mut out = HashMap::new();
    for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = f.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(v) = std::fs::read(&path).map(|b| serde_json::from_slice::<Value>(&b).unwrap_or_default()) else {
            continue;
        };
        let (Some(pid), Some(session)) = (v["pid"].as_u64(), v["sessionId"].as_str()) else { continue };
        let pid = pid as u32;
        // Windows writes the start as `procStartFt` (a FILETIME).
        if !alive(pid, v["procStart"].as_str().or(v["procStartFt"].as_str())) {
            continue;
        }
        let (pane, block) = ours.holder(pid).unwrap_or_else(|| scope_of(pid));
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        let mut l = Live {
            pid,
            kind: s("kind"),
            entrypoint: s("entrypoint"),
            status: s("status"),
            pane,
            block,
            place: String::new(),
            cwd: v["cwd"].as_str().map(str::to_owned),
        };
        l.place = l.place();
        out.insert(session.to_owned(), l);
    }
    out
}

/// The process is running, and is the one that wrote the file: it started
/// when `procStart` says.
fn alive(pid: u32, proc_start: Option<&str>) -> bool {
    if !crate::procinfo::alive(pid) {
        return false;
    }
    let Some(want) = proc_start else { return true };
    // Not readable (a sandbox): the pid alone.
    crate::procinfo::start_time(pid).is_none_or(|have| same_start(have, want))
}

/// Is `procStart` this start time ([`crate::procinfo::start_time`])?
/// Claude Code writes field 22 of `/proc/<pid>/stat` (clock ticks since
/// boot) on Linux, `LC_ALL=C TZ=UTC ps -o lstart=` (`Sat Oct  3 10:17:50
/// 2026`, to the second) on macOS, and on Windows `procStartFt`, the
/// creation time from `GetProcessTimes` (100 ns since 1601).
fn same_start(have: u64, want: &str) -> bool {
    if cfg!(target_os = "macos") { lstart_secs(want) == Some(have / 1_000_000) } else { want.parse() == Ok(have) }
}

/// `ps -o lstart=` in the C locale and UTC, in seconds since the epoch.
fn lstart_secs(s: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let [_, mon, day, time, year] = s.split_whitespace().collect::<Vec<_>>().try_into().ok()?;
    let mon = MONTHS.iter().position(|m| *m == mon)? + 1;
    let at = format!("{year}-{mon:02}-{:02}T{time}Z", day.parse::<u8>().ok()?);
    let ms = convert::at_ms(&at);
    (ms > 0).then_some(ms / 1000)
}

/// Our pane or agent block a process runs in, from its systemd scope
/// (`illogical-pane-<id>-<n>.scope`, `illogical-agent-<id>-<n>.scope`).
fn scope_of(pid: u32) -> (Option<PaneId>, Option<PaneId>) {
    let Ok(cg) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else { return (None, None) };
    let id = |prefix: &str| {
        let i = cg.find(prefix)? + prefix.len();
        cg[i..].split(['-', '.']).next()?.parse::<PaneId>().ok()
    };
    (id("illogical-pane-"), id("illogical-agent-"))
}

/// The desktop app's records, by Claude Code session id.
fn desktop_records(dir: &Path) -> HashMap<String, DesktopRecord> {
    let mut out = HashMap::new();
    let accounts = std::fs::read_dir(dir).into_iter().flatten().flatten();
    for org in accounts.flat_map(|a| std::fs::read_dir(a.path()).into_iter().flatten().flatten()) {
        for f in std::fs::read_dir(org.path()).into_iter().flatten().flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.starts_with("local_") || !name.ends_with(".json") {
                continue;
            }
            let Ok(v) = std::fs::read(f.path()).map(|b| serde_json::from_slice::<Value>(&b).unwrap_or_default()) else {
                continue;
            };
            let Some(id) = v["cliSessionId"].as_str() else { continue };
            out.insert(
                id.to_owned(),
                DesktopRecord {
                    title: v["title"].as_str().filter(|t| !t.is_empty()).map(str::to_owned),
                    archived: v["isArchived"] == true,
                },
            );
        }
    }
    out
}

/// The model option (`configOptions`' `model`) that is this model id:
/// `claude-opus-5-5` is "Opus 5.5" (`opus`), `claude-haiku-4-5-20251001` is
/// "Haiku 4.5". An option whose value is the id itself wins.
pub fn model_option(config_options: &Value, model: &str) -> Option<String> {
    let opts: Vec<&Value> =
        config_options.as_array()?.iter().find(|o| o["id"] == "model")?["options"].as_array()?.iter().collect();
    if let Some(o) = opts.iter().find(|o| o["value"] == model) {
        return o["value"].as_str().map(str::to_owned);
    }
    // claude-<family>-<major>-<minor>[-<date>]
    let parts: Vec<&str> = model.strip_prefix("claude-")?.split('-').collect();
    let family = parts.first()?;
    let nums: Vec<&str> =
        parts[1..].iter().copied().filter(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    let name = format!("{family} {}", nums.join(".")).to_lowercase();
    opts.iter().find(|o| o["name"].as_str().is_some_and(|n| n.to_lowercase() == name))?["value"]
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "illogical-conv-{name}-{}-{}",
            std::process::id(),
            crate::store::now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(format!("{}/tests/fixtures/conversations/{name}", env!("CARGO_MANIFEST_DIR")))
    }

    #[test]
    fn a_holder_in_another_daemons_pane_is_in_a_terminal() {
        // #77: the scope names pane 76, which only another daemon has.
        let l = Live {
            pid: 4242,
            kind: "interactive".into(),
            entrypoint: "cli".into(),
            status: "idle".into(),
            pane: Some(76),
            block: None,
            place: String::new(),
            cwd: None,
        };
        let theirs = l.clone().ours(|p| p == 3);
        assert_eq!((theirs.pane, theirs.block), (None, None));
        assert_eq!(theirs.place, "open in a terminal (pid 4242)");
        assert_eq!(l.clone().ours(|p| p == 76).place, "open in pane %76");
        let b = Live { pane: None, block: Some(76), ..l }.ours(|_| false);
        assert_eq!(b.place, "open in a terminal (pid 4242)");
    }

    #[test]
    fn indexes_titles_sources_and_forks() {
        let root = tmp("index");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let claude = root.join("claude");
        let proj = claude.join("projects/-work");
        std::fs::create_dir_all(&proj).unwrap();
        // S20's session and its fork, with their cwd pointed at a real folder.
        for f in ["a683c96a-c2b1-4ed7-bdd4-51b7d759125b", "d1ccc1e4-4b63-4b89-80b7-38128ee9d8cc"] {
            let text = std::fs::read_to_string(fixture(&format!("scratch/{f}.jsonl"))).unwrap();
            // Escaped as JSON: a Windows path's backslashes would break the line.
            let quoted = serde_json::to_string(work.to_str().unwrap()).unwrap();
            let text = text
                .replace("/home/user/illogical/spikes/s20-conversations/work/scratch", &quoted[1..quoted.len() - 1]);
            std::fs::write(proj.join(format!("{f}.jsonl")), text).unwrap();
        }
        // A subagent's transcript is in a folder of its own: not listed.
        std::fs::create_dir_all(proj.join("a683c96a-c2b1-4ed7-bdd4-51b7d759125b/subagents")).unwrap();
        std::fs::copy(
            fixture("shapes/subagent-transcript.jsonl"),
            proj.join("a683c96a-c2b1-4ed7-bdd4-51b7d759125b/subagents/agent-x.jsonl"),
        )
        .unwrap();
        // A session with no prompt isn't a conversation.
        std::fs::write(proj.join("empty.jsonl"), "{\"type\":\"queue-operation\"}\n").unwrap();
        let mut ix = Index::new(Dirs { claude: claude.clone(), desktop: root.join("desktop") });
        let all = ix.scan();
        assert_eq!(all.len(), 2, "{all:#?}");
        let orig = all.iter().find(|c| c.id.starts_with("a683")).unwrap();
        assert_eq!(orig.source, Source::Terminal);
        assert_eq!(orig.cwd, work.display().to_string());
        assert!(orig.cwd_exists);
        assert!(orig.first_prompt.as_deref().unwrap().starts_with("Remember the number 4817"));
        assert!(orig.model.as_deref().is_some_and(|m| m.starts_with("claude-")), "{orig:?}");
        let fork = all.iter().find(|c| c.id.starts_with("d1cc")).unwrap();
        assert_eq!(fork.forked_from.as_deref(), Some("a683c96a-c2b1-4ed7-bdd4-51b7d759125b"));
        assert_eq!(fork.title, "Canary word and note (fork)");
        assert!(shown(orig, false));
        // By prefix.
        assert_eq!(ix.find("a683").unwrap().id, orig.id);
        assert!(ix.find("zzz").is_err());

        // The folder goes: hidden unless everything is asked for.
        std::fs::remove_dir_all(&work).unwrap();
        let all = ix.scan();
        let orig = all.iter().find(|c| c.id.starts_with("a683")).unwrap();
        assert!(!orig.cwd_exists);
        assert!(!shown(orig, false) && shown(orig, true));

        // The desktop app's title and archive flag.
        let rec = root.join("desktop/claude-code-sessions/acct/org");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(
            rec.join("local_1.json"),
            json!({"cliSessionId": "a683c96a-c2b1-4ed7-bdd4-51b7d759125b", "title": "From the app", "isArchived": true}).to_string(),
        )
        .unwrap();
        let all = ix.scan();
        let orig = all.iter().find(|c| c.id.starts_with("a683")).unwrap();
        assert!(orig.archived);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_long_transcript_is_read_at_both_ends() {
        let root = tmp("long");
        let proj = root.join("claude/projects/-x");
        std::fs::create_dir_all(&proj).unwrap();
        let mut text = json!({"type":"user","cwd":"/x","entrypoint":"claude-desktop","timestamp":"2026-10-02T00:00:00Z","message":{"role":"user","content":"<system-reminder>scratch</system-reminder>\nfirst thing"}}).to_string();
        text.push('\n');
        let filler = json!({"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","content":[{"type":"text","text":"x".repeat(1000)}]}}).to_string();
        for _ in 0..200 {
            text.push_str(&filler);
            text.push('\n');
        }
        text.push_str(&json!({"type":"ai-title","aiTitle":"Late title"}).to_string());
        text.push('\n');
        std::fs::write(proj.join("s1.jsonl"), &text).unwrap();
        let mut ix = Index::new(Dirs { claude: root.join("claude"), desktop: root.join("d") });
        let c = &ix.scan()[0];
        assert_eq!((c.title.as_str(), c.first_prompt.as_deref()), ("Late title", Some("first thing")));
        assert_eq!(c.source, Source::Desktop);
        assert_eq!(c.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(c.created_ms, convert::at_ms("2026-10-02T00:00:00Z"));
        // A desktop session's scratch folder goes with it: still listed.
        assert!(!c.cwd_exists && shown(c, false));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    /// A process's `procStart`, as Claude Code writes it on this OS.
    fn proc_start(pid: u32) -> String {
        if cfg!(target_os = "macos") {
            let out = std::process::Command::new("ps")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        } else {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            stat.rsplit_once(')').unwrap().1.split_whitespace().nth(19).unwrap().to_owned()
        }
    }

    #[cfg(unix)]
    fn write_session(dir: &Path, name: &str, pid: u32, start: &str, sid: &str) {
        std::fs::write(
            dir.join(name),
            json!({"pid": pid, "sessionId": sid, "procStart": start, "kind": "interactive", "entrypoint": "cli", "status": "idle"}).to_string(),
        )
        .unwrap();
    }

    // Unix: starts processes with sh.
    #[cfg(unix)]
    #[test]
    fn live_sessions_are_checked_against_their_start_time() {
        let root = tmp("live");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let me = std::process::id();
        let start = proc_start(me);
        write_session(&sessions, "a.json", me, &start, "live-one");
        // The same pid, started at another time: someone else's now.
        let other = if cfg!(target_os = "macos") { "Thu Jan  1 00:00:01 1970" } else { "1" };
        write_session(&sessions, "b.json", me, other, "reused-pid");
        write_session(&sessions, "c.json", 999_999_999, &start, "gone");
        let l = live(&sessions, &Ours::default());
        let mut got: Vec<&str> = l.keys().map(String::as_str).collect();
        got.sort();
        assert_eq!(got, ["live-one"]);
        assert_eq!(l["live-one"].pid, me);
        assert!(l["live-one"].place().contains(&format!("pid {me}")) || l["live-one"].pane.is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Windows: Claude Code writes `procStartFt`, its creation FILETIME.
    #[cfg(windows)]
    #[test]
    fn live_sessions_on_windows_are_checked_against_their_filetime() {
        let root = tmp("live-win");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let me = std::process::id();
        let ft = crate::procinfo::start_time(me).unwrap().to_string();
        let write = |name: &str, pid: u32, ft: &str, sid: &str| {
            let v = json!({"pid": pid, "sessionId": sid, "procStartFt": ft, "kind": "interactive", "entrypoint": "cli", "status": "idle"});
            std::fs::write(sessions.join(name), v.to_string()).unwrap();
        };
        write("a.json", me, &ft, "live-one");
        write("b.json", me, "1", "reused-pid");
        write("c.json", 999_999_999, &ft, "gone");
        let l = live(&sessions, &Ours::default());
        assert_eq!(l.keys().map(String::as_str).collect::<Vec<_>>(), ["live-one"]);

        // Under one of our panes, by its ancestors.
        let ours = Ours { panes: [(crate::procinfo::ppid(me).unwrap(), 7)].into(), ..Default::default() };
        assert_eq!(live(&sessions, &ours)["live-one"].pane, Some(7));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ps_start_times() {
        assert_eq!(lstart_secs("Sat Oct  3 10:17:50 2026"), Some(1_791_022_670));
        assert_eq!(lstart_secs("Thu Jan  1 00:00:01 1970"), Some(1));
        assert_eq!(lstart_secs("Fri May 15 21:23:28 2026  "), Some(1_778_880_208));
        assert_eq!(lstart_secs("17819758"), None);
        assert_eq!(lstart_secs("Sat Foo  3 10:17:50 2026"), None);
        if cfg!(target_os = "macos") {
            assert!(same_start(1_791_022_670_123_456, "Sat Oct  3 10:17:50 2026"));
            assert!(!same_start(1_791_022_671_000_000, "Sat Oct  3 10:17:50 2026"));
        } else {
            assert!(same_start(17_819_758, "17819758"));
            assert!(!same_start(17_819_759, "17819758"));
        }
    }

    // Unix: starts processes with sh and reads their ancestry.
    #[cfg(unix)]
    #[test]
    fn a_holder_is_placed_by_its_ancestors() {
        let root = tmp("ours");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // A "pane" shell running a "Claude Code" (sleep) under it.
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(shell.stdout.take().unwrap()), &mut line).unwrap();
        let claude: u32 = line.trim().parse().unwrap();
        write_session(&sessions, "a.json", claude, &proc_start(claude), "in-pane");
        let me = std::process::id();
        write_session(&sessions, "b.json", me, &proc_start(me), "in-block");
        let ours = Ours { panes: [(shell.id(), 7)].into(), blocks: [(me, 9)].into() };
        let l = live(&sessions, &ours);
        assert_eq!((l["in-pane"].pane, l["in-pane"].block), (Some(7), None));
        assert_eq!(l["in-pane"].place, "open in pane %7");
        assert_eq!((l["in-block"].pane, l["in-block"].block), (None, Some(9)));
        // Not under any of ours.
        let l = live(&sessions, &Ours { panes: [(1, 7)].into(), ..Ours::default() });
        assert_eq!(l["in-pane"].pane.filter(|p| *p == 7), None);
        crate::procinfo::kill(claude);
        shell.wait().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn models_by_name() {
        let opts = json!([{"id":"model","options":[
            {"value":"default","name":"Default (recommended)"},
            {"value":"opus","name":"Opus 5.5"},
            {"value":"haiku","name":"Haiku 4.5"},
            {"value":"claude-opus-5","name":"Opus 5"},
        ]}]);
        assert_eq!(model_option(&opts, "claude-opus-5-5").as_deref(), Some("opus"));
        assert_eq!(model_option(&opts, "claude-haiku-4-5-20251001").as_deref(), Some("haiku"));
        assert_eq!(model_option(&opts, "claude-opus-5").as_deref(), Some("claude-opus-5"));
        assert_eq!(model_option(&opts, "claude-sonnet-9").as_deref(), None);
        assert_eq!(model_option(&json!([]), "claude-opus-5-5"), None);
    }
}
