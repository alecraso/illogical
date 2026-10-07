//! What survives the daemon: the layout, and each pane's output and
//! terminal state.
//!
//! ```text
//! $XDG_STATE_HOME/arugula/          0700
//!   layout.json                       sessions, tabs, splits, pane details,
//!                                     machines
//!   blocks/<id>/                      every block type (terminals: below)
//!     seg-<offset>.log                raw output; the name is the stream
//!                                     offset of its first byte
//!     index                           "<offset> resize <cols> <rows>",
//!                                     "<offset> restore <unix ms>"
//!     checkpoint                      "offset <n>\n" + engine checkpoint
//!     exec.json                       a VM pane's session on its machine
//! ```
//!
//! The log is the truth. A checkpoint is a cache of the terminal at some
//! log offset, so a restore replays only the log after it; one that can't be
//! used is ignored and the log tail is replayed instead.
//!
//! Scrollback holds secrets (pasted tokens), so everything is private to the
//! user and bounded by retention.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use arugula_core::Mux;
use arugula_proto::{BlockType, Machine, MachineId, PaneId, Policy, api::HistoryKind};
use serde::{Deserialize, Serialize};

pub const LAYOUT_VERSION: u32 = 1;
const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const RETAIN_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    pub saved_at_ms: u64,
    pub mux: Mux,
    pub panes: BTreeMap<PaneId, PaneMeta>,
    #[serde(default)]
    pub machines: BTreeMap<MachineId, Machine>,
    /// Machine ids aren't reused: a sprite named after a deleted machine
    /// could still be on its way out.
    #[serde(default)]
    pub next_machine: MachineId,
}

/// What a restore needs to know about a pane beyond its place in the layout.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneMeta {
    pub policy: Policy,
    pub cwd: Option<String>,
    pub command: Option<String>,
    /// Shell integration for this pane's shells (`None`: the default, on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<bool>,
    /// The machine it runs on; `None` is this host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<MachineId>,
    /// What the block is (a terminal unless it says otherwise).
    #[serde(default, rename = "type", skip_serializing_if = "is_terminal")]
    pub kind: BlockType,
    /// Keep the pane when its program ends (`arugula run`), across
    /// restarts too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold: bool,
    /// A non-terminal block's config: what makes it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Never shown to anyone but the owner (M14).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Started through MCP (M16): by which client, for which agent block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<arugula_proto::StartedBy>,
    /// The guest (principal id) behind it: they started it, or an agent
    /// of theirs did. Such an agent asks the owner for nothing in their
    /// name (#234's invites).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<String>,
    /// The agent conversation running in it (#146), for a restart to
    /// resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSession>,
    /// Someone picked its restart policy (a pane left at the default that
    /// runs Claude Code resumes its conversation).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub policy_set: bool,
}

/// An agent's conversation in a pane (#146), as its hooks or Claude
/// Code's session files said.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// `claude`, `codex`.
    pub agent: String,
    /// Its session id: checked by [`crate::resume::valid_id`] before it's
    /// kept, and passed as an argument, never as shell text.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// Where it runs: it's resumed there (the pane's shell may be
    /// elsewhere, after `cd dir && claude`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The agent was running in the pane when last looked: a restart
    /// resumes only then.
    #[serde(default)]
    pub running: bool,
}

fn is_terminal(k: &BlockType) -> bool {
    *k == BlockType::Terminal
}

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

pub fn private_dir(path: &Path) -> io::Result<()> {
    crate::perm::dir_mode(fs::DirBuilder::new().recursive(true), 0o700).create(path)
}

fn private_file() -> OpenOptions {
    let mut o = OpenOptions::new();
    crate::perm::open_mode(&mut o, 0o600);
    o
}

/// Write a file so that it is either entirely old or entirely new, even
/// across a crash or power loss.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = private_file().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    // The rename itself, on disk. (Windows can't open a directory as a
    // file; NTFS journals the rename.)
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl StateDir {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        private_dir(&root)?;
        // Blocks of every type live in `blocks/` (`panes/` before M6).
        // Programs that outlived the old daemon still write their exit
        // records under `panes/`, so it stays as a link.
        let old = root.join("panes");
        if old.is_dir() && !old.is_symlink() && !root.join("blocks").exists() {
            fs::rename(&old, root.join("blocks"))?;
            // Only Unix daemons ever had `panes/`.
            #[cfg(unix)]
            std::os::unix::fs::symlink("blocks", &old)?;
        }
        private_dir(&root.join("blocks"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pane_dir(&self, pane: PaneId) -> PathBuf {
        self.root.join("blocks").join(pane.to_string())
    }

    pub fn load_layout(&self) -> io::Result<Option<Saved>> {
        let bytes = match fs::read(self.root.join("layout.json")) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let saved: Saved = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if saved.version != LAYOUT_VERSION {
            return Err(io::Error::other(format!("layout.json version {} (want {LAYOUT_VERSION})", saved.version)));
        }
        Ok(Some(saved))
    }

    pub fn save_layout(&self, saved: &Saved) -> io::Result<()> {
        write_atomic(&self.root.join("layout.json"), &serde_json::to_vec_pretty(saved).map_err(io::Error::other)?)
    }

    /// Pane directories with no pane in the layout (closed while the daemon
    /// was down, or left by a crash) are retired like closed panes.
    pub fn remove_strays(&self, keep: &[PaneId]) {
        let Ok(entries) = fs::read_dir(self.root.join("blocks")) else {
            return;
        };
        for e in entries.flatten() {
            let id = e.file_name().to_str().and_then(|n| n.parse::<PaneId>().ok());
            if let Some(id) = id.filter(|id| !keep.contains(id)) {
                retire_dir(&e.path(), id);
            }
        }
    }

    /// Delete closed panes' history older than `max_age_ms`.
    pub fn prune_closed(&self, max_age_ms: u64) {
        let Ok(entries) = fs::read_dir(self.root.join("closed")) else { return };
        let cutoff = now_ms().saturating_sub(max_age_ms);
        for e in entries.flatten() {
            let closed_at = e.file_name().to_str().and_then(|n| n.rsplit_once('-')?.1.parse::<u64>().ok());
            if closed_at.is_some_and(|t| t < cutoff) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }

    /// Every pane directory with history: open panes, then closed ones.
    pub fn pane_dirs(&self) -> Vec<(PaneId, bool, PathBuf)> {
        let mut out = Vec::new();
        for (sub, open) in [("blocks", true), ("closed", false)] {
            let Ok(entries) = fs::read_dir(self.root.join(sub)) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.split('-').next().and_then(|n| n.parse::<PaneId>().ok());
                if let Some(id) = id {
                    out.push((id, open, e.path()));
                }
            }
        }
        out.sort_by_key(|(id, open, _)| (!*open, *id));
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "e", rename_all = "snake_case")]
pub enum Event {
    Resize {
        cols: u16,
        rows: u16,
    },
    Restore {
        at_ms: u64,
    },
    /// Wall time at this offset (written at most once a second of output),
    /// for timing exports.
    Time {
        at_ms: u64,
    },
    /// A prompt was drawn.
    Prompt {
        at_ms: u64,
    },
    /// A command started; its output follows.
    Command {
        at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Who typed it (M13).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        /// What it is; none is a command (older records, shells).
        #[serde(default, skip_serializing_if = "HistoryKind::is_command")]
        kind: HistoryKind,
    },
    /// Someone else started typing here (M13): one record per handoff, not
    /// per keystroke.
    Driver {
        at_ms: u64,
        who: String,
    },
    /// The command that started last finished.
    End {
        at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<i32>,
    },
    Cwd {
        path: String,
    },
    Notify {
        at_ms: u64,
        title: String,
        body: String,
    },
    Bell {
        at_ms: u64,
    },
}

/// One index line: `{"o":offset,"e":kind,...}`.
#[derive(Serialize, Deserialize)]
struct Line {
    o: u64,
    #[serde(flatten)]
    event: Event,
}

/// One pane's output on disk: append-only segments, an index of events by
/// offset, and the latest checkpoint.
pub struct PaneLog {
    dir: PathBuf,
    /// Start offsets of the segments, oldest first.
    segments: Vec<u64>,
    current: Option<File>,
    end: u64,
    retain: u64,
}

impl PaneLog {
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        Self::open_with(dir, RETAIN_BYTES)
    }

    pub fn open_with(dir: PathBuf, retain: u64) -> io::Result<Self> {
        private_dir(&dir)?;
        let mut segments: Vec<u64> = fs::read_dir(&dir)?
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_prefix("seg-")?.strip_suffix(".log")?.parse().ok())
            .collect();
        segments.sort_unstable();
        let end = match segments.last() {
            Some(start) => start + fs::metadata(dir.join(seg_name(*start)))?.len(),
            None => 0,
        };
        Ok(Self { dir, segments, current: None, end, retain })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stream offset just past the last byte written.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Oldest offset still on disk.
    pub fn start(&self) -> u64 {
        self.segments.first().copied().unwrap_or(self.end)
    }

    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        let mut data = data;
        while !data.is_empty() {
            let seg_start = match self.segments.last() {
                Some(s) if self.end - s < SEGMENT_BYTES => *s,
                _ => {
                    self.segments.push(self.end);
                    self.current = None;
                    self.end
                }
            };
            if self.current.is_none() {
                let path = self.dir.join(seg_name(seg_start));
                self.current = Some(private_file().create(true).append(true).open(&path)?);
            }
            let room = (SEGMENT_BYTES - (self.end - seg_start)) as usize;
            let (now, rest) = data.split_at(room.min(data.len()));
            self.current.as_mut().unwrap().write_all(now)?;
            self.end += now.len() as u64;
            data = rest;
        }
        self.enforce_retention();
        Ok(())
    }

    fn enforce_retention(&mut self) {
        while self.segments.len() > 1 && self.end - self.segments[0] > self.retain {
            let _ = fs::remove_file(self.dir.join(seg_name(self.segments[0])));
            self.segments.remove(0);
        }
    }

    pub fn sync(&mut self) -> io::Result<()> {
        if let Some(f) = &self.current {
            f.sync_data()?;
        }
        Ok(())
    }

    /// The bytes from `from` (clamped to what is still on disk) to the end.
    pub fn read_from(&self, from: u64) -> io::Result<(u64, Vec<u8>)> {
        let from = from.clamp(self.start(), self.end);
        let mut out = Vec::with_capacity((self.end - from) as usize);
        for (i, start) in self.segments.iter().enumerate() {
            let next = self.segments.get(i + 1).copied().unwrap_or(self.end);
            if next <= from {
                continue;
            }
            let mut f = File::open(self.dir.join(seg_name(*start)))?;
            f.seek(SeekFrom::Start(from.saturating_sub(*start)))?;
            f.read_to_end(&mut out)?;
        }
        Ok((from, out))
    }

    /// At most `max` bytes from `from` (clamped to what is on disk).
    pub fn read_range(&self, from: u64, max: usize) -> io::Result<(u64, Vec<u8>)> {
        let from = from.clamp(self.start(), self.end);
        let want = ((self.end - from) as usize).min(max);
        let mut out = Vec::with_capacity(want);
        for (i, start) in self.segments.iter().enumerate() {
            let next = self.segments.get(i + 1).copied().unwrap_or(self.end);
            if next <= from || out.len() >= want {
                continue;
            }
            let mut f = File::open(self.dir.join(seg_name(*start)))?;
            f.seek(SeekFrom::Start(from.max(*start) - start))?;
            let room = (want - out.len()) as u64;
            f.take(room).read_to_end(&mut out)?;
        }
        Ok((from, out))
    }

    pub fn record(&mut self, offset: u64, event: Event) -> io::Result<()> {
        let mut line = serde_json::to_vec(&Line { o: offset, event }).map_err(io::Error::other)?;
        line.push(b'\n');
        let path = self.dir.join("index");
        private_file().create(true).append(true).open(&path)?.write_all(&line)
    }

    pub fn events(&self) -> Vec<(u64, Event)> {
        read_events(&self.dir)
    }

    pub fn save_checkpoint(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.sync()?;
        let mut out = format!("offset {offset}\n").into_bytes();
        out.extend_from_slice(bytes);
        write_atomic(&self.dir.join("checkpoint"), &out)
    }

    pub fn load_checkpoint(&self) -> Option<(u64, Vec<u8>)> {
        let bytes = fs::read(self.dir.join("checkpoint")).ok()?;
        let nl = bytes.iter().position(|b| *b == b'\n')?;
        let offset = std::str::from_utf8(&bytes[..nl]).ok()?.strip_prefix("offset ")?.parse().ok()?;
        Some((offset, bytes[nl + 1..].to_vec()))
    }

    /// Forget all history; the stream carries on from the same offset.
    pub fn purge(&mut self) -> io::Result<()> {
        self.current = None;
        for s in self.segments.drain(..) {
            let _ = fs::remove_file(self.dir.join(seg_name(s)));
        }
        let _ = fs::remove_file(self.dir.join("index"));
        let _ = fs::remove_file(self.dir.join("checkpoint"));
        Ok(())
    }

    /// The pane closed: keep its history a while (for `arugula history`
    /// and `search`) under `closed/<id>-<time>`.
    pub fn retire(self, pane: PaneId) {
        // Closed first: Windows won't move a directory with open files.
        let dir = self.dir.clone();
        drop(self);
        retire_dir(&dir, pane);
    }
}

/// A pane directory's index. Reads M2's space-separated lines too.
pub fn read_events(dir: &Path) -> Vec<(u64, Event)> {
    let Ok(text) = fs::read_to_string(dir.join("index")) else {
        return vec![];
    };
    parse_events(&text)
}

/// An index's lines (a synced copy's, say), as events.
pub fn parse_events(text: &str) -> Vec<(u64, Event)> {
    text.lines()
        .filter_map(|l| {
            if l.starts_with('{') {
                let line: Line = serde_json::from_str(l).ok()?;
                return Some((line.o, line.event));
            }
            let mut w = l.split_whitespace();
            let offset = w.next()?.parse().ok()?;
            let event = match w.next()? {
                "resize" => Event::Resize { cols: w.next()?.parse().ok()?, rows: w.next()?.parse().ok()? },
                "restore" => Event::Restore { at_ms: w.next()?.parse().ok()? },
                _ => return None,
            };
            Some((offset, event))
        })
        .collect()
}

/// Days a closed pane's history is kept.
pub const CLOSED_RETENTION_MS: u64 = 7 * 24 * 3600 * 1000;

fn retire_dir(dir: &Path, pane: PaneId) {
    let Some(root) = dir.parent().and_then(Path::parent) else { return };
    let closed = root.join("closed");
    if private_dir(&closed).is_err() || fs::rename(dir, closed.join(format!("{pane}-{}", now_ms()))).is_err() {
        let _ = fs::remove_dir_all(dir);
    }
}

fn seg_name(start: u64) -> String {
    format!("seg-{start:020}.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arugula-store-{name}-{}-{}", std::process::id(), now_ms()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn log_appends_rotates_reads_and_reopens() {
        let dir = tmp("log");
        let mut log = PaneLog::open(dir.clone()).unwrap();
        let chunk = vec![b'a'; 3 * 1024 * 1024];
        log.append(&chunk).unwrap();
        log.append(b"hello").unwrap();
        log.append(&chunk).unwrap();
        assert_eq!(log.end(), 6 * 1024 * 1024 + 5);
        assert_eq!(log.segments, vec![0, SEGMENT_BYTES]);
        let (from, bytes) = log.read_from(3 * 1024 * 1024).unwrap();
        assert_eq!(from, 3 * 1024 * 1024);
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(bytes.len() as u64, log.end() - from);
        // Bounded reads, across a segment boundary.
        let (from, part) = log.read_range(SEGMENT_BYTES - 2, 4).unwrap();
        assert_eq!((from, part.as_slice()), (SEGMENT_BYTES - 2, &b"aaaa"[..]));
        let (_, tail) = log.read_range(3 * 1024 * 1024, 1 << 30).unwrap();
        assert_eq!(tail, bytes);
        drop(log);
        let log = PaneLog::open(dir.clone()).unwrap();
        assert_eq!(log.end(), 6 * 1024 * 1024 + 5);
        // Modes are Unix's; Windows has the profile's ACL.
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(dir.join(seg_name(0))).unwrap().permissions())
                & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retention_drops_oldest_segments() {
        let dir = tmp("retain");
        let mut log = PaneLog::open_with(dir.clone(), SEGMENT_BYTES * 2).unwrap();
        for _ in 0..5 {
            log.append(&vec![b'x'; SEGMENT_BYTES as usize]).unwrap();
        }
        assert_eq!(log.segments.len(), 2);
        assert_eq!(log.start(), 3 * SEGMENT_BYTES);
        assert_eq!(log.read_from(0).unwrap().0, 3 * SEGMENT_BYTES, "clamped to what is kept");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn index_and_checkpoint_round_trip() {
        let dir = tmp("index");
        let mut log = PaneLog::open(dir.clone()).unwrap();
        log.record(0, Event::Resize { cols: 80, rows: 24 }).unwrap();
        log.record(42, Event::Restore { at_ms: 7 }).unwrap();
        let cmd = Event::Command {
            at_ms: 9,
            text: Some("echo \"a;b\"".into()),
            cwd: None,
            by: None,
            kind: HistoryKind::Command,
        };
        log.record(50, cmd.clone()).unwrap();
        assert_eq!(
            log.events(),
            vec![(0, Event::Resize { cols: 80, rows: 24 }), (42, Event::Restore { at_ms: 7 }), (50, cmd)]
        );
        // M2's format still reads.
        fs::write(dir.join("index"), "0 resize 80 24\n60 restore 5\n").unwrap();
        assert_eq!(log.events(), vec![(0, Event::Resize { cols: 80, rows: 24 }), (60, Event::Restore { at_ms: 5 })]);
        log.save_checkpoint(42, b"state\nbytes").unwrap();
        assert_eq!(log.load_checkpoint(), Some((42, b"state\nbytes".to_vec())));
        log.append(b"abc").unwrap();
        log.purge().unwrap();
        assert_eq!(log.load_checkpoint(), None);
        assert!(log.events().is_empty());
        log.append(b"def").unwrap();
        assert_eq!(log.read_from(0).unwrap(), (3, b"def".to_vec()), "offsets carry on after a purge");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn layout_saves_atomically_and_privately() {
        let dir = tmp("layout");
        let state = StateDir::open(dir.clone()).unwrap();
        assert_eq!(state.load_layout().unwrap(), None);
        let mut mux = Mux::new();
        mux.apply(arugula_core::Intent::NewSession { name: None, from_pane: None }).unwrap();
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: 1,
            mux,
            panes: [(
                1,
                PaneMeta { policy: Policy::Rerun { confirm: true }, cwd: Some("/tmp".into()), ..Default::default() },
            )]
            .into(),
            machines: BTreeMap::new(),
            next_machine: 1,
        };
        state.save_layout(&saved).unwrap();
        assert_eq!(state.load_layout().unwrap(), Some(saved));
        // Modes are Unix's; Windows has the profile's ACL.
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&dir).unwrap().permissions()) & 0o777, 0o700);
        fs::remove_dir_all(dir).unwrap();
    }
}
